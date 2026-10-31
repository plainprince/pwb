//! Network framing and system domain protocol message types (pwb-core::net).
//!
//! PlainWeb transport is serverless. System domains (search.plain, gossip.plain,
//! nodes.plain, register.plain) provide native network services directly without
//! central servers.
//!
//! This module provides the NodeEngine which consolidates peer table management,
//! routing logic, framing serialization, and socket event loops. Both pwb-node
//! and pwb-browser instantiate this engine to ensure they run on the exact same
//! network stack.
//!
//! Every datagram is carried in a signed envelope ([`envelope`]): the sender
//! Ed25519-signs the framing plus payload, so receivers can attribute a
//! message to a key instead of trusting a field inside it, and a per-sender
//! sliding window rejects replays. Envelopes are fragmented to fit a 1200-byte
//! datagram and reassembled on receipt.
//!
//! Payloads are both authenticated and encrypted for one recipient with
//! ephemeral X25519 + ChaCha20-Poly1305 ([`envelope`]).
//!
//! Onion routing ([`crate::onion`]) is wired into the same socket: a 512-byte
//! [`crate::onion::OnionCell`] is detected before envelope decoding and peeled
//! hop-by-hop. Relays re-originate the cell from their own socket, so a
//! receiving hop only observes the previous relay's address; exits deliver a
//! signed, multi-cell message to [`NodeEngine::subscribe_onion`]. Relayed cells
//! are reported through [`NodeEngine::subscribe_relay`] so the node layer can
//! meter and mint relay credits.

mod addr;
mod cell_channel;
mod e2ee;
mod engine;
mod envelope;
mod loops;
mod messages;
mod onion_transport;

#[cfg(test)]
mod tests;

pub use addr::{map_dest_for, prefer_family, unmap_source};
pub use cell_channel::CellChannel;
pub use e2ee::E2eeState;
pub use engine::{
    peer_exchange_loop, resolve_bootstrap_peer, BootstrapPeer, NodeConfig, NodeEngine, PeerInfo,
    RendezvousEvent,
};
pub use messages::{
    blog_entry_sign_message, decode_artifact_chunk, decode_punch_probe,
    domain_claim_funding_sign_message, domain_claim_sign_message, encode_artifact_chunk,
    encode_punch_probe, gossip_announce_sign_message, index_proof_sign_message, is_valid_room_name,
    posting_sign_message, postings_sign_message, postings_snippet_sign_message,
    punch_probe_sign_message, room_post_sign_message, shard_query_sign_message,
    shard_response_sign_message, shard_snippets_sign_message, verify_claim_signature,
    BalanceAttestationWire, BlindClaimWire, BlogEntryWire, DomainRecordWire, IndexProofWire,
    IssuerBindingWire, NetworkError, NetworkMessage, OwnershipProofWire, PostingWire,
    QueryReceiptWire, QuorumCertificateWire, RoomPostWire, SearchResultWire, SendFlags,
    SnippetWire, SyncRecordWire, Transport, VoucherBlindRequestWire, VoucherWire, BLOG_ENTRY_HOPS,
    BLOG_ENTRY_PREFIX, BLOG_HISTORY_MAX, DOMAIN_CLAIM_FUNDING_PREFIX, DOMAIN_CLAIM_PREFIX,
    GOSSIP_ANNOUNCE_PREFIX, MAX_ARTIFACT_CHUNK, MAX_BLOG_TEXT, MAX_BLOG_TITLE, MAX_CHAT_TEXT,
    MAX_ROOM_NAME, POSTINGS_BATCH_MAX, POSTINGS_PREFIX, POSTING_PREFIX, PUNCH_MAGIC,
    ROOM_HISTORY_MAX, ROOM_POST_HOPS, ROOM_POST_PREFIX, SHARD_RESPONSE_PREFIX,
};
pub use onion_transport::{OnionKind, OnionMessage, OnionRequest, OnionReturnPath, RelayEvent};
