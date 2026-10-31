//! Signed service receipts: query, index, and relay.

use std::collections::HashMap;

use pwb_core::crypto::{ContentId, Keypair, PublicKey, Signature};
use pwb_core::wire::Bundle;

use crate::search;

use super::signing::{
    index_sign_message, query_sign_message, relay_sign_message, rendezvous_sign_message,
};

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

/// Signed seal over one window of rendezvous (seed) work.
///
/// A seed is paid for admitting newcomers and for handing out its peer table,
/// neither of which any other node can do on its behalf: only the node that
/// authenticated the newcomer and sent the `PeerExchange` can seal them. The
/// receipt is self-signed exactly like [`RelayReceipt`], so it proves what this
/// node claims to have served, not that a peer watched it happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendezvousReceipt {
    /// The seed that served the window (the account that will mint).
    pub seed: PublicKey,
    /// Window index (monotone, starts at 0).
    pub window: u64,
    /// Newcomers admitted in the window.
    pub joins: u64,
    /// Peer-table bytes served in the window (once per recipient).
    pub peer_bytes: u64,
    /// Rolling hash-chain head over the window's rendezvous events.
    pub chain_head: [u8; 32],
    /// Seed signature over the canonical encoding of the fields above.
    pub signature: Signature,
}

impl RendezvousReceipt {
    /// Sign a window seal.
    pub fn sign(
        seed: &Keypair,
        window: u64,
        joins: u64,
        peer_bytes: u64,
        chain_head: [u8; 32],
    ) -> Self {
        let message = rendezvous_sign_message(seed.public, window, joins, peer_bytes, chain_head);
        RendezvousReceipt {
            seed: seed.public,
            window,
            joins,
            peer_bytes,
            chain_head,
            signature: seed.sign(&message),
        }
    }

    /// Verify the seed signature over the canonical encoding.
    pub fn verify(&self) -> bool {
        let message = rendezvous_sign_message(
            self.seed,
            self.window,
            self.joins,
            self.peer_bytes,
            self.chain_head,
        );
        self.signature.verify(&self.seed, &message)
    }

    /// Evidence digest binding this receipt into an attestor ledger chain.
    pub fn evidence_digest(&self) -> [u8; 32] {
        let message = rendezvous_sign_message(
            self.seed,
            self.window,
            self.joins,
            self.peer_bytes,
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
