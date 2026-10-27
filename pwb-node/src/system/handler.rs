//! System domain request handler.

use std::net::SocketAddr;

use crate::chat::ChatSnapshot;
use crate::domain::DomainRegistry;
use crate::host::ArtifactHost;
use crate::identity::IdentityManager;
use crate::room::RoomSnapshot;
use crate::search::SearchEngine;
use pwb_core::crypto::PublicKey;
use pwb_core::net::{NetworkMessage, SearchResultWire};

/// A live snapshot of the node's network view, rendered by `network.plain`.
///
/// The browser and the node fill in only what they know: missing addresses
/// render as "unknown" instead of failing the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkSnapshot {
    /// The UDP address the engine is listening on.
    pub listen_addr: Option<SocketAddr>,
    /// Where LAN discovery is bound (broadcast/multicast).
    pub discovery_addr: Option<SocketAddr>,
    /// Public address learned via STUN, when NAT traversal is configured.
    pub public_addr: Option<SocketAddr>,
    /// Peers in the live peer table.
    pub peers: Vec<(PublicKey, SocketAddr)>,
}

pub struct SystemDomainHandler<'a> {
    pub search: &'a SearchEngine,
    pub host: &'a ArtifactHost,
    pub registry: &'a DomainRegistry,
    pub identity: &'a IdentityManager,
    /// Spendable balance from the signed credit ledger, rendered on
    /// `register.plain`.
    pub balance: u64,
    /// Live network view for `network.plain`; defaults to identity/registry
    /// figures only.
    pub network: NetworkSnapshot,
    /// Chat state for `chat.plain`; defaults to this identity with no
    /// transcripts.
    pub chat: ChatSnapshot,
    /// Public-room state for `chat.plain?room=`; defaults to no rooms.
    pub room: RoomSnapshot,
}

impl<'a> SystemDomainHandler<'a> {
    pub fn new(
        search: &'a SearchEngine,
        host: &'a ArtifactHost,
        registry: &'a DomainRegistry,
        identity: &'a IdentityManager,
        balance: u64,
    ) -> Self {
        Self {
            search,
            host,
            registry,
            identity,
            balance,
            network: NetworkSnapshot::default(),
            chat: ChatSnapshot {
                local: identity.public_key(),
                ..ChatSnapshot::default()
            },
            room: RoomSnapshot {
                local: identity.public_key(),
                ..RoomSnapshot::default()
            },
        }
    }

    /// Attach a live network snapshot (builder form, so existing callers keep
    /// compiling and `network.plain` degrades gracefully).
    pub fn with_network(mut self, network: NetworkSnapshot) -> Self {
        self.network = network;
        self
    }

    /// Attach chat state (builder form, mirroring [`Self::with_network`]).
    pub fn with_chat(mut self, chat: ChatSnapshot) -> Self {
        self.chat = chat;
        self
    }

    /// Attach public-room state (builder form, same shape as
    /// [`Self::with_chat`]).
    pub fn with_room(mut self, room: RoomSnapshot) -> Self {
        self.room = room;
        self
    }

    /// Process incoming system domain requests and return response messages.
    pub fn handle_message(&self, message: NetworkMessage) -> Option<NetworkMessage> {
        match message {
            NetworkMessage::SearchQuery { query, .. } => {
                let raw_results = self.search.search(&query);
                let wire_results = raw_results
                    .into_iter()
                    .map(|r| SearchResultWire {
                        content_id: r.content_id,
                        title: r.title,
                        rank_score: r.rank_score,
                        snippet: format!("Content ID: {}", r.content_id),
                    })
                    .collect();

                Some(NetworkMessage::SearchResponse {
                    query,
                    results: wire_results,
                })
            }
            NetworkMessage::FetchArtifact { content_id } => {
                let bundle_bytes = self.host.get(&content_id).unwrap_or_default();
                Some(NetworkMessage::ArtifactResponse {
                    content_id,
                    bundle_bytes,
                })
            }
            _ => None,
        }
    }
}
