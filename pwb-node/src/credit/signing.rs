//! Canonical sign-message encodings for credit receipts.

use pwb_core::crypto::{ContentId, PublicKey};

/// Domain-separation prefix for every signed credit message.
const CREDIT_SIGN_PREFIX: &[u8] = b"PWBCREDIT1";

pub(super) fn query_sign_message(
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

pub(super) fn relay_sign_message(
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
