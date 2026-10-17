//! Message protocol framing, transport trait, and network errors.

use std::net::SocketAddr;

use crate::crypto::{ContentId, PublicKey, Signature};
use crate::onion::OnionError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Wire format representation of a search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResultWire {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
    pub snippet: String,
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

    /// Request a binary .pwb artifact bundle by its content ID.
    FetchArtifact { content_id: ContentId },

    /// Response containing the full .pwb binary artifact bundle.
    ArtifactResponse {
        content_id: ContentId,
        bundle_bytes: Vec<u8>,
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
    ClaimDomain {
        domain: String,
        target_content_id: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        signature: Signature,
    },
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

    #[error("Timeout")]
    Timeout,

    #[error(transparent)]
    Onion(#[from] OnionError),
}
