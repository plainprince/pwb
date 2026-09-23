//! Peer Verifier for Anti-Sybil Proof of Useful Work (`pwb-node::verifier`).

use pwb_core::crypto::{ContentId, PublicKey, Signature};

#[derive(Debug, Clone)]
pub struct IndexProof {
    pub miner: PublicKey,
    pub content_id: ContentId,
    pub term_hash: ContentId,
    pub signature: Signature,
}

pub struct PeerVerifier;

impl PeerVerifier {
    pub fn verify_indexing_proof(proof: &IndexProof) -> bool {
        // Verify miner signature over indexing work
        let mut msg = Vec::new();
        msg.extend_from_slice(&proof.content_id.0);
        msg.extend_from_slice(&proof.term_hash.0);

        proof.signature.verify(&proof.miner, &msg)
    }
}
