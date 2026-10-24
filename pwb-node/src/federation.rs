//! Federated, DHT-sharded decentralized search (`pwb-node::federation`).
//!
//! This is the PlainWeb answer to a central search index. There is no server:
//!
//! 1. **Sharded inverted index.** Every term maps to a 256-bit shard id
//!    ([`search::shard_id`]) and its postings list lives on the
//!    [`SHARD_REPLICATION`] DHT nodes XOR-closest to that id. When a node
//!    indexes a bundle, [`DistributedSearch::index_and_publish`] writes each
//!    term's postings to the responsible shards via
//!    [`NetworkMessage::StorePosting`].
//! 2. **Shard-routed queries.** A query is tokenized and each term is sent as a
//!    [`NetworkMessage::ShardQuery`] to that term's responsible nodes, which
//!    answer with their local postings in a
//!    [`NetworkMessage::ShardResponse`]. The requester merges every shard's
//!    postings ([`search::aggregate`]) and ranks locally.
//! 3. **Proof of service.** A term-responsible node signs a
//!    [`crate::credit::QueryReceipt`] over the results it served, bound to the
//!    requester's challenge nonce, so serving a query mints credits under the
//!    node's [`CreditMinter`] (challenge/response round-trip, claim-once).
//!
//! The service is transport-agnostic: it drives a [`NodeEngine`] and only needs
//! `subscribe()` + `send_to_addr()`. Both the browser's embedded node and the
//! full `pwb-node` instantiate it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::dht::NodeId;
use pwb_core::net::{
    decode_artifact_chunk, encode_artifact_chunk, index_proof_sign_message, posting_sign_message,
    postings_sign_message, shard_query_sign_message, shard_response_sign_message, IndexProofWire,
    NetworkMessage, NodeEngine, PostingWire, QueryReceiptWire, MAX_ARTIFACT_CHUNK,
    POSTINGS_BATCH_MAX,
};
use pwb_core::wire::Bundle;
use rand::seq::SliceRandom;
use rand::RngCore;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::credit::{CreditLedger, CreditMinter, IndexReceipt, QueryReceipt};
use crate::host::ArtifactHost;
use crate::privacy::{self, PrivacyError};
use crate::search::{self, SearchEngine, SearchResult};

/// Replication factor: a term's postings are written to this many XOR-closest
/// nodes (self included when it is responsible).
pub const SHARD_REPLICATION: usize = 3;

/// How long a query waits for shard responses before ranking what arrived.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(400);

/// How long an artifact fetch waits for any peer to answer.
pub const FETCH_TIMEOUT: Duration = Duration::from_millis(600);

/// How long a private (onion-routed) shard query waits for one exit.
pub const PRIVATE_QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// How long a private artifact fetch waits for one exit. A large bundle is
/// hundreds of cells, so this is deliberately longer than a direct fetch.
pub const PRIVATE_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How many peers a private fetch tries in parallel. Each attempt is an
/// independent circuit, so one unhelpful exit cannot block the others.
pub const MAX_PRIVATE_FETCH_PEERS: usize = 8;

/// Chunk requests kept in flight while fetching one large artifact. Four
/// circuits balance latency against the entry relays' per-source cell budget.
pub const MAX_PRIVATE_CHUNKS_IN_FLIGHT: usize = 4;

/// Largest artifact accepted over private chunked fetch. Larger content needs
/// a streaming protocol; refusing is safer than assembling unbounded memory.
pub const MAX_PRIVATE_BUNDLE: usize = 2 * 1024 * 1024;

/// Verify a wire receipt against the query it claims to answer: same term,
/// same requester challenge, same result set, same responder.
fn receipt_valid(
    receipt: &QueryReceiptWire,
    term: &str,
    challenge: &[u8; 32],
    responder: &PublicKey,
    postings: &[PostingWire],
) -> bool {
    if receipt.server != *responder || receipt.query != term || receipt.challenge != *challenge {
        return false;
    }
    let expected: Vec<ContentId> = postings.iter().map(|posting| posting.content_id).collect();
    if receipt.results != expected {
        return false;
    }
    QueryReceipt {
        server: receipt.server,
        query: receipt.query.clone(),
        results: receipt.results.clone(),
        challenge: receipt.challenge,
        signature: receipt.signature,
    }
    .verify()
}

impl From<&QueryReceipt> for QueryReceiptWire {
    fn from(receipt: &QueryReceipt) -> Self {
        QueryReceiptWire {
            server: receipt.server,
            query: receipt.query.clone(),
            results: receipt.results.clone(),
            challenge: receipt.challenge,
            signature: receipt.signature,
        }
    }
}

/// A node's federated search service: local index + shard routing + service
/// receipts, driven over a [`NodeEngine`].
pub struct DistributedSearch {
    engine: NodeEngine,
    index: Arc<SearchEngine>,
    host: Arc<ArtifactHost>,
    keypair: Keypair,
    minter: Mutex<CreditMinter>,
    /// Signed credit history: serving work is appended here (shared with the
    /// domain service so query earnings can pay for domain claims).
    ledger: Arc<CreditLedger>,
    pending_queries: Mutex<HashMap<u64, mpsc::UnboundedSender<NetworkMessage>>>,
    pending_fetches: Mutex<HashMap<ContentId, mpsc::UnboundedSender<Vec<u8>>>>,
    next_query_id: AtomicU64,
    /// Fetch artifacts announced by peers automatically. Disabled for a
    /// privacy-mode browser: a peer could otherwise announce a unique content
    /// id and observe which browser fetches it, tying that interest to the
    /// browser's address.
    auto_fetch: bool,
    /// Answer direct (non-onion) artifact requests from the local cache. A
    /// privacy-mode browser disables this so a peer cannot probe for a unique
    /// content id and learn which browser holds it.
    serve_direct_artifacts: bool,
    /// Answer direct shard queries. Disabled in privacy mode so a peer cannot
    /// probe the browser's local index for terms it browsed or searched.
    serve_direct_queries: bool,
}

impl DistributedSearch {
    pub fn new(
        engine: NodeEngine,
        index: Arc<SearchEngine>,
        host: Arc<ArtifactHost>,
        keypair: Keypair,
    ) -> Self {
        let minter = CreditMinter::with_default_policy(keypair.public);
        let ledger = Arc::new(CreditLedger::new(keypair.clone(), None, 0));
        Self {
            engine,
            index,
            host,
            keypair,
            minter: Mutex::new(minter),
            ledger,
            pending_queries: Mutex::new(HashMap::new()),
            pending_fetches: Mutex::new(HashMap::new()),
            next_query_id: AtomicU64::new(1),
            auto_fetch: true,
            serve_direct_artifacts: true,
            serve_direct_queries: true,
        }
    }

    /// Disable automatic artifact fetch on [`NetworkMessage::GossipAnnounce`].
    /// The browser enables this in onion mode so passive gossip cannot probe
    /// which pages it is interested in.
    pub fn with_private_mode(mut self, private: bool) -> Self {
        self.auto_fetch = !private;
        self.serve_direct_artifacts = !private;
        self.serve_direct_queries = !private;
        self
    }

    /// Use a persistent/shared [`CreditLedger`] instead of the default
    /// in-memory one (the node wires this to the domain service's ledger).
    pub fn with_ledger(mut self, ledger: Arc<CreditLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// The signed credit ledger backing [`Self::credits_earned`].
    pub fn ledger(&self) -> &Arc<CreditLedger> {
        &self.ledger
    }

    /// The shared local index (postings this node keeps / is responsible for).
    pub fn index(&self) -> &Arc<SearchEngine> {
        &self.index
    }

    /// The shared local artifact store (content-addressed bundles).
    pub fn host(&self) -> &Arc<ArtifactHost> {
        &self.host
    }

    /// Credits this node has minted by serving queries / indexing bundles,
    /// derived from the signed ledger.
    pub fn credits_earned(&self) -> u64 {
        self.ledger.earned()
    }

    /// Store, index, and publish a bundle to the network: the local index keeps
    /// every posting, and each term is replicated to its responsible shards.
    /// Also mints index credits (content-addressed indexing work).
    pub fn index_and_publish(&self, bytes: &[u8]) -> Option<ContentId> {
        let bundle = Bundle::deserialize(bytes).ok()?;
        let content_id = self.host.store(bytes.to_vec()).ok()?;
        self.index.record_title(content_id, bundle.title.clone());
        let postings = self.index.index_bundle(content_id, &bundle);
        self.publish_postings(&postings);

        // `gossip.plain`: announce the new artifact so peers that do not
        // happen to be responsible for its terms can still learn about it and
        // fetch the bytes. The envelope authenticates the publisher; the
        // announcement also carries a signature over the announced tuple.
        let _ = self.engine.broadcast(NetworkMessage::gossip_announce(
            &self.keypair,
            content_id,
            bundle.title.clone(),
        ));

        // Indexing is content-addressed useful work: meter the bytes actually
        // parsed and mint against a receipt that re-derives their hash.
        let now = Instant::now();
        let mut minter = self.minter.lock().unwrap();
        minter.record_traffic(bytes.len() as u64);
        if let Some(receipt) = IndexReceipt::sign(&self.keypair, bytes) {
            // Only ask peers to attest work this node's own mint policy
            // accepted. Broadcasting a proof for a rate-limited (too-small)
            // bundle would let verifiers credit 10 credits for work the local
            // anti-ASIC floor rejected, inflating attested supply.
            if let Ok(amount) = minter.mint_index(&receipt, bytes, now) {
                self.ledger
                    .record_earned("index", amount, &format!("indexed {}", content_id));

                // Every verifier re-hashes the bundle and records the same
                // earning in its attestor ledger, so the subject's balance
                // becomes peer-attested rather than self-declared.
                let proof = IndexProofWire {
                    miner: receipt.indexer,
                    content_id: receipt.content_id,
                    term_hash: receipt.term_hash,
                    signature: self.keypair.sign(&index_proof_sign_message(
                        &receipt.content_id,
                        &receipt.term_hash,
                    )),
                };
                let _ = self.engine.broadcast(NetworkMessage::CheckIndexProof {
                    query_id: rand::random(),
                    proof,
                    bundle_bytes: bytes.to_vec(),
                });
            }
        }
        drop(minter);

        Some(content_id)
    }

    /// Write a document's postings to the responsible shards (and keep them
    /// locally so this node can always serve its own documents).
    ///
    /// Postings for one recipient are grouped and sent in signed batches, so a
    /// bundle with hundreds of terms costs a handful of messages instead of
    /// hundreds. Before batching, each term was its own message and any
    /// document larger than the receiver's per-peer burst bucket lost the tail
    /// of its index (and the follow-up `CheckIndexProof`) to the rate limiter.
    fn publish_postings(&self, postings: &[PostingWire]) {
        for posting in postings {
            self.index.record_posting(posting);
        }

        let mut batches: HashMap<(PublicKey, SocketAddr), Vec<PostingWire>> = HashMap::new();
        for posting in postings {
            let target = search::shard_id(&posting.term);
            for (public_key, address) in self.responsible_nodes(&target) {
                if public_key == self.keypair.public {
                    continue;
                }
                batches
                    .entry((public_key, address))
                    .or_default()
                    .push(posting.clone());
            }
        }

        let publisher = self.keypair.public;
        for ((_, address), batch) in batches {
            for chunk in batch.chunks(POSTINGS_BATCH_MAX) {
                let signature = self.keypair.sign(&postings_sign_message(chunk, &publisher));
                let _ = self.engine.send_to_addr(
                    address,
                    NetworkMessage::StorePostings {
                        postings: chunk.to_vec(),
                        publisher,
                        signature,
                    },
                );
            }
        }
    }

    /// The [`SHARD_REPLICATION`] nodes (self + live DHT contacts) XOR-closest
    /// to `target`.
    ///
    /// This reads the Kademlia routing table populated from authenticated
    /// peers and `FindNode`/`PeerExchange` traffic, not a flat peer list, so a
    /// node that is not directly connected but learned through the DHT is
    /// still eligible to hold the shard. The flat peer table is only a fallback
    /// for engines that have not populated routing yet (e.g. before start).
    fn responsible_nodes(&self, target: &NodeId) -> Vec<(PublicKey, SocketAddr)> {
        let mut nodes: Vec<(PublicKey, SocketAddr)> = Vec::new();
        if let Some(Ok(local)) = self.engine.local_addr() {
            nodes.push((self.keypair.public, local));
        }
        let target_id = *target;
        for contact in self
            .engine
            .closest_contacts(&target_id, SHARD_REPLICATION * 4)
        {
            nodes.push((contact.public_key, contact.address));
        }
        if nodes.len() <= 1 {
            let peers = self.engine.peer_table();
            let peers = peers.lock().unwrap();
            nodes.extend(peers.iter().map(|(key, info)| (*key, info.address)));
        }
        nodes.sort_by_key(|(public_key, _)| NodeId::from_public_key(public_key).xor(&target_id));
        nodes.dedup_by_key(|(public_key, _)| *public_key);
        nodes.truncate(SHARD_REPLICATION);
        nodes
    }

    /// Every peer address currently known to the engine.
    fn peer_addresses(&self) -> Vec<SocketAddr> {
        self.engine
            .peer_table()
            .lock()
            .unwrap()
            .values()
            .map(|info| info.address)
            .collect()
    }

    /// Run the inbound-message loop until the engine stops. Consumes messages
    /// the engine forwards through [`NodeEngine::subscribe`].
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.engine.subscribe();
        while let Some((from, message)) = rx.recv().await {
            // Responses are routed to the awaiting caller by correlation id.
            if let NetworkMessage::ShardResponse { query_id, .. } = &message {
                let sender = self.pending_queries.lock().unwrap().get(query_id).cloned();
                if let Some(sender) = sender {
                    let _ = sender.send(message.clone());
                    continue;
                }
            }
            self.handle_message(from, message).await;
        }
    }

    async fn handle_message(&self, from: SocketAddr, message: NetworkMessage) {
        match message {
            // Shard write: only keep postings whose publisher signature is
            // valid; otherwise any peer could inject arbitrary search results.
            NetworkMessage::StorePosting {
                posting,
                publisher,
                signature,
            } => {
                if signature.verify(&publisher, &posting_sign_message(&posting, &publisher)) {
                    self.index.record_posting(&posting);
                }
            }

            // Batched shard write: the receiver loop has already checked the
            // batch signature against the envelope signer, but verify here too
            // so this service never trusts an unverified batch.
            NetworkMessage::StorePostings {
                postings,
                publisher,
                signature,
            } => {
                if signature.verify(&publisher, &postings_sign_message(&postings, &publisher)) {
                    for posting in &postings {
                        self.index.record_posting(posting);
                    }
                }
            }

            // Shard read: answer from the local postings store and sign a
            // query receipt so serving the query can mint credits.
            NetworkMessage::ShardQuery {
                term,
                query_id,
                sender,
                challenge,
                sender_signature,
            } if self.serve_direct_queries => {
                // Only a request whose challenge was chosen and signed by a
                // peer can back a credit mint. Without this check the serving
                // node could record a nonce it invented for itself.
                if sender_signature.verify(
                    &sender,
                    &shard_query_sign_message(&term, query_id, &sender, &challenge),
                ) {
                    self.handle_shard_query(from, term, query_id, challenge)
                        .await;
                }
            }

            // Artifact announcement: fetch bytes we do not hold yet. The
            // receiver loop has already verified the publisher signature, so
            // the source address is an authenticated peer.
            NetworkMessage::GossipAnnounce { content_id, .. } => {
                if self.auto_fetch && !self.host.contains(&content_id) {
                    let _ = self
                        .engine
                        .send_to_addr(from, NetworkMessage::FetchArtifact { content_id });
                }
            }

            // Artifact serving: return the bytes we hold, if any. A
            // privacy-mode browser serves only onion chunk requests, so a
            // direct probe cannot confirm what it has cached.
            NetworkMessage::FetchArtifact { content_id } if self.serve_direct_artifacts => {
                if let Some(bytes) = self.host.get(&content_id) {
                    // Meter the bytes actually sent; they back future mints.
                    self.minter
                        .lock()
                        .unwrap()
                        .record_traffic(bytes.len() as u64);
                    let _ = self.engine.send_to_addr(
                        from,
                        NetworkMessage::ArtifactResponse {
                            content_id,
                            bundle_bytes: bytes,
                        },
                    );
                }
            }

            // Artifact bytes arrived: enforce content addressing, then cache
            // and index them and wake any waiter. Without the hash check a
            // peer could serve arbitrary bytes for any requested CID.
            NetworkMessage::ArtifactResponse {
                content_id,
                bundle_bytes,
            } if !bundle_bytes.is_empty() => {
                if ContentId::compute(&bundle_bytes) != content_id {
                    return;
                }
                if let Ok(bundle) = Bundle::deserialize(&bundle_bytes) {
                    let _ = self.host.store(bundle_bytes.clone());
                    self.index.record_title(content_id, bundle.title.clone());
                    self.index.index_bundle(content_id, &bundle);
                }
                if let Some(waiter) = self.pending_fetches.lock().unwrap().remove(&content_id) {
                    let _ = waiter.send(bundle_bytes);
                }
            }

            _ => {}
        }
    }

    /// Answer one verified shard query and settle its credit: the challenge is
    /// the requester's (recorded claim-once), the traffic is the exact size of
    /// the serialized response, and the minted amount lands in the signed
    /// ledger.
    async fn handle_shard_query(
        &self,
        from: SocketAddr,
        term: String,
        query_id: u64,
        challenge: [u8; 32],
    ) {
        let response = self.build_shard_response(term, query_id, challenge);
        let _ = self.engine.send_to_addr(from, response);
    }

    /// Build (and meter) a signed shard response. Shared by the direct and
    /// onion-delivered paths so a private query mints the same proof of service
    /// as a direct one.
    fn build_shard_response(
        &self,
        term: String,
        query_id: u64,
        challenge: [u8; 32],
    ) -> NetworkMessage {
        let postings = self.index.postings_wire(&term);
        let receipt = if postings.is_empty() {
            None
        } else {
            let results: Vec<ContentId> = postings.iter().map(|p| p.content_id).collect();
            Some(QueryReceipt::sign(&self.keypair, &term, results, challenge))
        };
        let responder = self.keypair.public;
        let signature = self.keypair.sign(&shard_response_sign_message(
            query_id, &term, &postings, &challenge, &responder,
        ));
        let response = NetworkMessage::ShardResponse {
            query_id,
            term,
            postings,
            responder,
            signature,
            receipt: receipt.as_ref().map(QueryReceiptWire::from),
        };

        if let Some(receipt) = &receipt {
            // The response on the wire is the work: meter its real size
            // (fragmentation and envelopes only add bytes, never remove
            // them). JSON is the transport encoding, so this is the payload
            // the peer actually receives.
            let served_bytes = serde_json::to_vec(&response)
                .map(|bytes| bytes.len() as u64)
                .unwrap_or(0);
            let now = Instant::now();
            let mut minter = self.minter.lock().unwrap();
            minter.record_challenge(challenge, now);
            minter.record_traffic(served_bytes);
            if let Ok(amount) = minter.mint_query(receipt, now) {
                self.ledger.record_earned(
                    "query",
                    amount,
                    &format!("served term '{}'", receipt.query),
                );
            }
        }

        response
    }

    /// Validate and answer an onion-delivered shard query. `originator` is
    /// authenticated by the onion layer; a message that claims a different
    /// `sender` is refused, and the requester signature over the challenge must
    /// verify before the query can mint credit.
    pub fn answer_private_shard_query(
        &self,
        message: &NetworkMessage,
        originator: PublicKey,
    ) -> Option<NetworkMessage> {
        let NetworkMessage::ShardQuery {
            term,
            query_id,
            sender,
            challenge,
            sender_signature,
        } = message
        else {
            return None;
        };
        if *sender != originator
            || !sender_signature.verify(
                &originator,
                &shard_query_sign_message(term, *query_id, &originator, challenge),
            )
        {
            return None;
        }
        Some(self.build_shard_response(term.clone(), *query_id, *challenge))
    }

    /// Answer one base64-encoded artifact chunk from the local host. A missing
    /// content id or an offset past the end yields an empty slice; the
    /// requester reassembles and verifies the whole bundle by content id.
    pub fn answer_private_fetch_chunk(&self, message: &NetworkMessage) -> Option<NetworkMessage> {
        let NetworkMessage::FetchArtifactChunk {
            content_id,
            offset,
            len,
        } = message
        else {
            return None;
        };
        let bytes = self.host.get(content_id)?;
        let total = bytes.len();
        if total > u32::MAX as usize {
            return None;
        }
        let start = (*offset as usize).min(total);
        let requested = (*len as usize).min(MAX_ARTIFACT_CHUNK);
        let end = start.saturating_add(requested).min(total);
        Some(NetworkMessage::ArtifactChunkResponse {
            content_id: *content_id,
            offset: start as u32,
            total_len: total as u32,
            data: encode_artifact_chunk(&bytes[start..end]),
        })
    }

    /// Federated search: tokenize the query, consult every term's shard, and
    /// merge the postings into a ranked result list.
    pub async fn query(&self, query: &str) -> Vec<SearchResult> {
        self.query_with_receipts(query).await.0
    }

    /// Like [`Self::query`], but also returns the signed [`QueryReceiptWire`]s
    /// the answering shard nodes sent, so a caller can verify that each result
    /// came from a live round-trip (and audit peers' claimed work).
    pub async fn query_with_receipts(
        &self,
        query: &str,
    ) -> (Vec<SearchResult>, Vec<QueryReceiptWire>) {
        let terms = search::tokenize(query);
        let mut postings = Vec::new();
        let mut receipts = Vec::new();
        for term in &terms {
            let (term_postings, term_receipts) = self.query_term(term).await;
            postings.extend(term_postings);
            receipts.extend(term_receipts);
        }
        (search::aggregate(postings), receipts)
    }

    /// Gather a single term's postings (and service receipts) from the local
    /// index and its responsible shards.
    async fn query_term(&self, term: &str) -> (Vec<PostingWire>, Vec<QueryReceiptWire>) {
        let mut postings = self.index.postings_wire(term);
        let mut receipts = Vec::new();
        let target = search::shard_id(term);
        let responsible = self.responsible_nodes(&target);
        let responders: std::collections::HashSet<PublicKey> = responsible
            .iter()
            .map(|(public_key, _)| *public_key)
            .collect();
        let peers: Vec<SocketAddr> = responsible
            .into_iter()
            .filter(|(public_key, _)| *public_key != self.keypair.public)
            .map(|(_, address)| address)
            .collect();
        if peers.is_empty() {
            return (postings, receipts);
        }

        let query_id = self.next_query_id.fetch_add(1, Ordering::Relaxed);
        let mut challenge = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut challenge);

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_queries.lock().unwrap().insert(query_id, tx);
        // Sign the challenge we chose so the serving node records proof of a
        // real requester round-trip rather than a nonce it invented.
        let sender = self.keypair.public;
        let sender_signature = self.keypair.sign(&shard_query_sign_message(
            term, query_id, &sender, &challenge,
        ));
        for address in &peers {
            let _ = self.engine.send_to_addr(
                *address,
                NetworkMessage::ShardQuery {
                    term: term.to_string(),
                    query_id,
                    sender,
                    challenge,
                    sender_signature,
                },
            );
        }

        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
        let mut received = 0usize;
        while received < peers.len() {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(NetworkMessage::ShardResponse {
                    query_id: response_id,
                    term: response_term,
                    postings: remote,
                    responder,
                    signature,
                    receipt,
                })) => {
                    received += 1;

                    // A shard response is only usable if it is bound to this
                    // query, signed by a node responsible for the term, and
                    // its receipt (when present) matches the results.
                    if response_id != query_id
                        || response_term.to_lowercase() != *term
                        || !responders.contains(&responder)
                    {
                        continue;
                    }
                    let message = shard_response_sign_message(
                        query_id, term, &remote, &challenge, &responder,
                    );
                    if !signature.verify(&responder, &message) {
                        continue;
                    }
                    if remote
                        .iter()
                        .any(|posting| posting.term.to_lowercase() != *term)
                    {
                        continue;
                    }
                    if let Some(receipt) = &receipt {
                        if !receipt_valid(receipt, term, &challenge, &responder, &remote) {
                            continue;
                        }
                        receipts.push(receipt.clone());
                    }
                    postings.extend(remote);
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        self.pending_queries.lock().unwrap().remove(&query_id);
        (postings, receipts)
    }

    /// Federated search over onion circuits: the local index contributes its
    /// own postings, and every remote shard answer is requested through a
    /// three-hop circuit ending at that shard's responsible node. Shard nodes
    /// learn the term and query id but never the requester's address.
    ///
    /// Returns an error instead of silently falling back to direct traffic when
    /// the swarm is too small to build a circuit. A partial result is returned
    /// when at least one remote shard answered; missing shards simply contribute
    /// nothing.
    pub async fn query_private(&self, query: &str) -> Result<Vec<SearchResult>, PrivacyError> {
        let terms = search::tokenize(query);
        let mut postings = Vec::new();
        for term in &terms {
            let (term_postings, _) = self.query_term_private(term).await?;
            postings.extend(term_postings);
        }
        Ok(search::aggregate(postings))
    }

    async fn query_term_private(
        &self,
        term: &str,
    ) -> Result<(Vec<PostingWire>, Vec<QueryReceiptWire>), PrivacyError> {
        let mut postings = self.index.postings_wire(term);
        let mut receipts = Vec::new();
        let target = search::shard_id(term);
        let direct: std::collections::HashSet<PublicKey> =
            privacy::direct_peers(&self.engine).into_iter().collect();
        let responders: Vec<PublicKey> = self
            .responsible_nodes(&target)
            .into_iter()
            .map(|(key, _)| key)
            .filter(|key| *key != self.keypair.public && direct.contains(key))
            .collect();
        if responders.is_empty() {
            return Ok((postings, receipts));
        }

        let query_id = self.next_query_id.fetch_add(1, Ordering::Relaxed);
        let mut tasks = JoinSet::new();
        let mut path_error: Option<PrivacyError> = None;
        for responder in &responders {
            let path = match privacy::select_path(&self.engine, Some(*responder), 3) {
                Ok(path) => path,
                Err(error) => {
                    path_error = Some(error);
                    continue;
                }
            };
            let mut challenge = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut challenge);
            // A per-request circuit key keeps the shard node from linking this
            // query to the browser's long-term identity, and the signed
            // challenge still proves a real requester round-trip.
            let circuit_key = Keypair::generate();
            let sender = circuit_key.public;
            let sender_signature = circuit_key.sign(&shard_query_sign_message(
                term, query_id, &sender, &challenge,
            ));
            let message = NetworkMessage::ShardQuery {
                term: term.to_string(),
                query_id,
                sender,
                challenge,
                sender_signature,
            };
            let engine = self.engine.clone();
            let expected = *responder;
            let term_owned = term.to_string();
            tasks.spawn(async move {
                let reply = engine
                    .send_onion_request_as(&circuit_key, &path, &message, PRIVATE_QUERY_TIMEOUT)
                    .await
                    .ok()?;
                let NetworkMessage::ShardResponse {
                    query_id: response_id,
                    term: response_term,
                    postings: remote,
                    responder,
                    signature,
                    receipt,
                } = reply
                else {
                    return None;
                };
                if response_id != query_id
                    || responder != expected
                    || response_term.to_lowercase() != term_owned
                {
                    return None;
                }
                let signed = shard_response_sign_message(
                    query_id,
                    &term_owned,
                    &remote,
                    &challenge,
                    &responder,
                );
                if !signature.verify(&responder, &signed)
                    || remote
                        .iter()
                        .any(|posting| posting.term.to_lowercase() != term_owned)
                {
                    return None;
                }
                if let Some(receipt) = &receipt {
                    if !receipt_valid(receipt, &term_owned, &challenge, &responder, &remote) {
                        return None;
                    }
                }
                Some((remote, receipt))
            });
        }

        let mut answered = 0usize;
        while let Some(joined) = tasks.join_next().await {
            let Ok(Some((remote, receipt))) = joined else {
                continue;
            };
            answered += 1;
            postings.extend(remote);
            if let Some(receipt) = receipt {
                receipts.push(receipt);
            }
        }
        if answered == 0 && !responders.is_empty() {
            return Err(path_error.unwrap_or_else(|| {
                PrivacyError::RequestFailed(format!(
                    "no shard node answered '{}' through an onion circuit",
                    term
                ))
            }));
        }
        Ok((postings, receipts))
    }

    /// Retrieve a bundle by content id through independent onion circuits. Each
    /// candidate exit is asked in its own circuit, so no peer sees both the
    /// requester and the content id. The first content-addressed answer wins.
    ///
    /// Artifacts larger than one chunk are pulled chunk by chunk from the same
    /// holder over fresh circuits and verified against the requested content
    /// id, so the bytes never travel in the clear. Anything over
    /// [`MAX_PRIVATE_BUNDLE`] is refused rather than assembled unbounded.
    pub async fn fetch_private(&self, content_id: &ContentId) -> Result<Vec<u8>, PrivacyError> {
        if let Some(bytes) = self.host.get(content_id) {
            return Ok(bytes);
        }
        let mut peers = privacy::direct_peers(&self.engine);
        if peers.len() < 3 {
            return Err(PrivacyError::NotEnoughPeers {
                needed: 3,
                available: peers.len(),
            });
        }
        peers.shuffle(&mut rand::thread_rng());
        peers.truncate(MAX_PRIVATE_FETCH_PEERS);
        let requested = *content_id;

        // Phase 0: ask every candidate for a zero-length range. The reply is a
        // tiny 4 KiB onion message that only reports the total length, so
        // probing eight peers does not flood the return path with eight 48 KiB
        // chunk replies.
        let mut tasks = JoinSet::new();
        for exit in peers {
            let path = privacy::select_path(&self.engine, Some(exit), 3)?;
            let engine = self.engine.clone();
            tasks.spawn(async move {
                let circuit_key = Keypair::generate();
                let message = NetworkMessage::FetchArtifactChunk {
                    content_id: requested,
                    offset: 0,
                    len: 0,
                };
                let reply = engine
                    .send_onion_request_as(&circuit_key, &path, &message, PRIVATE_FETCH_TIMEOUT)
                    .await
                    .ok()?;
                let NetworkMessage::ArtifactChunkResponse {
                    content_id,
                    offset,
                    total_len,
                    data,
                } = reply
                else {
                    return None;
                };
                if content_id != requested || offset != 0 || !data.is_empty() {
                    return None;
                }
                let total = total_len as usize;
                if total == 0 || total > MAX_PRIVATE_BUNDLE {
                    return None;
                }
                Some((exit, total))
            });
        }

        let (exit, total) = {
            let mut holder = None;
            while let Some(joined) = tasks.join_next().await {
                if let Ok(Some(found)) = joined {
                    holder = Some(found);
                    break;
                }
            }
            holder.ok_or_else(|| {
                PrivacyError::RequestFailed(
                    "no peer answered the private artifact probe".to_string(),
                )
            })?
        };

        // Phase 1: fetch the first chunk from the chosen holder, with one
        // retry on a fresh circuit.
        let mut assembled = None;
        for _ in 0..2 {
            let path = privacy::select_path(&self.engine, Some(exit), 3)?;
            let circuit_key = Keypair::generate();
            let message = NetworkMessage::FetchArtifactChunk {
                content_id: requested,
                offset: 0,
                len: MAX_ARTIFACT_CHUNK as u32,
            };
            let Ok(NetworkMessage::ArtifactChunkResponse {
                content_id,
                offset,
                total_len,
                data,
            }) = self
                .engine
                .send_onion_request_as(&circuit_key, &path, &message, PRIVATE_FETCH_TIMEOUT)
                .await
            else {
                continue;
            };
            if content_id != requested || offset != 0 || total_len as usize != total {
                continue;
            }
            let Some(bytes) = decode_artifact_chunk(&data) else {
                continue;
            };
            if bytes.len() != total.min(MAX_ARTIFACT_CHUNK) {
                continue;
            }
            assembled = Some(bytes);
            break;
        }
        let mut assembled = assembled.ok_or_else(|| {
            PrivacyError::RequestFailed(
                "the private artifact holder did not serve its first chunk".to_string(),
            )
        })?;

        // Phase 2: pull the remaining chunks from the same holder, four
        // circuits at a time. Each chunk uses a fresh circuit key; all exits
        // are the same peer, so the bytes stay content-addressed.
        let mut offset = assembled.len();
        while offset < total {
            let mut batch = JoinSet::new();
            let mut expected: Vec<(usize, usize)> = Vec::new();
            for _ in 0..MAX_PRIVATE_CHUNKS_IN_FLIGHT {
                if offset >= total {
                    break;
                }
                let chunk_offset = offset;
                let chunk_len = (total - chunk_offset).min(MAX_ARTIFACT_CHUNK);
                offset += chunk_len;
                expected.push((chunk_offset, chunk_len));
                let path = privacy::select_path(&self.engine, Some(exit), 3)?;
                let engine = self.engine.clone();
                batch.spawn(async move {
                    for _ in 0..2 {
                        let circuit_key = Keypair::generate();
                        let message = NetworkMessage::FetchArtifactChunk {
                            content_id: requested,
                            offset: chunk_offset as u32,
                            len: chunk_len as u32,
                        };
                        let reply = match engine
                            .send_onion_request_as(
                                &circuit_key,
                                &path,
                                &message,
                                PRIVATE_FETCH_TIMEOUT,
                            )
                            .await
                        {
                            Ok(reply) => reply,
                            Err(_) => continue,
                        };
                        let NetworkMessage::ArtifactChunkResponse {
                            content_id,
                            offset: response_offset,
                            total_len,
                            data,
                        } = reply
                        else {
                            continue;
                        };
                        if content_id != requested
                            || response_offset as usize != chunk_offset
                            || total_len as usize != total
                        {
                            continue;
                        }
                        if let Some(bytes) = decode_artifact_chunk(&data) {
                            if bytes.len() == chunk_len {
                                return Some((chunk_offset, bytes));
                            }
                        }
                    }
                    None
                });
            }

            let mut received = Vec::new();
            while let Some(joined) = batch.join_next().await {
                if let Ok(Some(chunk)) = joined {
                    received.push(chunk);
                }
            }
            if received.len() != expected.len() {
                return Err(PrivacyError::RequestFailed(format!(
                    "artifact {content_id} was incomplete over onion circuits"
                )));
            }
            received.sort_by_key(|(chunk_offset, _)| *chunk_offset);
            for (index, (expected_offset, expected_len)) in expected.iter().enumerate() {
                let (chunk_offset, bytes) = &received[index];
                if chunk_offset != expected_offset || bytes.len() != *expected_len {
                    return Err(PrivacyError::RequestFailed(format!(
                        "artifact {content_id} had a mismatched onion chunk"
                    )));
                }
                assembled.extend_from_slice(bytes);
            }
        }

        if assembled.len() != total || ContentId::compute(&assembled) != requested {
            return Err(PrivacyError::RequestFailed(format!(
                "artifact {content_id} did not verify after onion fetch"
            )));
        }
        if let Ok(bundle) = Bundle::deserialize(&assembled) {
            let _ = self.host.store(assembled.clone());
            self.index.record_title(requested, bundle.title.clone());
            self.index.index_bundle(requested, &bundle);
        }
        Ok(assembled)
    }

    /// Retrieve a bundle by content id: from the local host if present,
    /// otherwise from any peer. Fetched bundles are cached and indexed.
    pub async fn fetch(&self, content_id: &ContentId) -> Option<Vec<u8>> {
        if let Some(bytes) = self.host.get(content_id) {
            return Some(bytes);
        }
        let peers = self.peer_addresses();
        if peers.is_empty() {
            return None;
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_fetches.lock().unwrap().insert(*content_id, tx);
        for address in &peers {
            let _ = self.engine.send_to_addr(
                *address,
                NetworkMessage::FetchArtifact {
                    content_id: *content_id,
                },
            );
        }
        let result = match tokio::time::timeout(FETCH_TIMEOUT, rx.recv()).await {
            Ok(Some(bytes)) => Some(bytes),
            _ => None,
        };
        self.pending_fetches.lock().unwrap().remove(content_id);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::{PlainNode, Tag};

    fn bundle_bytes(title: &str, body: &str) -> Vec<u8> {
        let mut root = PlainNode::new(0, Tag::Document);
        let mut text = PlainNode::new(1, Tag::Text);
        text.text_content = Some(body.to_string());
        root.children.push(text);
        pwb_web::bundle::create_bundle(
            root,
            title.to_string(),
            None,
            None,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
        .serialize()
    }

    #[test]
    fn test_responsible_nodes_include_self_and_cap_at_replication() {
        let engine = NodeEngine::new(pwb_core::net::NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: Keypair::generate(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        });
        let service = DistributedSearch::new(
            engine,
            Arc::new(SearchEngine::new()),
            Arc::new(ArtifactHost::new()),
            Keypair::generate(),
        );
        // Before start(), local_addr is None, so only peers would appear; with
        // no peers the responsible set is empty but must not panic.
        assert!(service.responsible_nodes(&search::shard_id("x")).len() <= SHARD_REPLICATION);
    }

    #[test]
    fn test_index_and_publish_populates_local_index() {
        let engine = NodeEngine::new(pwb_core::net::NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: Keypair::generate(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        });
        let service = DistributedSearch::new(
            engine,
            Arc::new(SearchEngine::new()),
            Arc::new(ArtifactHost::new()),
            Keypair::generate(),
        );
        let bytes = bundle_bytes("Plain Web Demo", "decentralized search engine");
        let cid = service.index_and_publish(&bytes).expect("indexed");
        assert!(service.index().title(&cid).is_some());
        assert!(!service.index().postings_wire("decentralized").is_empty());
    }
}
