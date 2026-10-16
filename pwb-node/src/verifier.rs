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

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::crypto::Keypair;

    fn proof_for(key: &Keypair) -> IndexProof {
        let content_id = ContentId([7; 32]);
        let term_hash = ContentId([9; 32]);
        let mut msg = Vec::new();
        msg.extend_from_slice(&content_id.0);
        msg.extend_from_slice(&term_hash.0);
        IndexProof {
            miner: key.public,
            content_id,
            term_hash,
            signature: key.sign(&msg),
        }
    }

    #[test]
    fn test_valid_indexing_proof_verifies() {
        let key = Keypair::generate();
        assert!(PeerVerifier::verify_indexing_proof(&proof_for(&key)));
    }

    #[test]
    fn test_proof_signed_by_another_key_fails() {
        let miner = Keypair::generate();
        let impostor = Keypair::generate();
        let mut proof = proof_for(&miner);
        // Swap in another identity's signature over the same message.
        let mut msg = Vec::new();
        msg.extend_from_slice(&proof.content_id.0);
        msg.extend_from_slice(&proof.term_hash.0);
        proof.signature = impostor.sign(&msg);
        assert!(!PeerVerifier::verify_indexing_proof(&proof));
    }

    #[test]
    fn test_tampered_content_or_term_hash_fails() {
        let key = Keypair::generate();
        let mut proof = proof_for(&key);
        proof.content_id = ContentId([8; 32]);
        assert!(!PeerVerifier::verify_indexing_proof(&proof));

        let mut proof = proof_for(&key);
        proof.term_hash = ContentId([1; 32]);
        assert!(!PeerVerifier::verify_indexing_proof(&proof));
    }

    #[test]
    fn test_tampered_miner_identity_fails() {
        let key = Keypair::generate();
        let mut proof = proof_for(&key);
        proof.miner = Keypair::generate().public;
        assert!(!PeerVerifier::verify_indexing_proof(&proof));
    }

    #[test]
    fn test_garbage_signature_fails() {
        let key = Keypair::generate();
        let mut proof = proof_for(&key);
        proof.signature = Signature([0; 64]);
        assert!(!PeerVerifier::verify_indexing_proof(&proof));
    }
}
