//! System domain request handler.

use crate::domain::DomainRegistry;
use crate::host::ArtifactHost;
use crate::identity::IdentityManager;
use crate::search::SearchEngine;
use pwb_core::net::{NetworkMessage, SearchResultWire};

pub struct SystemDomainHandler<'a> {
    pub search: &'a SearchEngine,
    pub host: &'a ArtifactHost,
    pub registry: &'a DomainRegistry,
    pub identity: &'a IdentityManager,
}

impl<'a> SystemDomainHandler<'a> {
    pub fn new(
        search: &'a SearchEngine,
        host: &'a ArtifactHost,
        registry: &'a DomainRegistry,
        identity: &'a IdentityManager,
    ) -> Self {
        Self {
            search,
            host,
            registry,
            identity,
        }
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
                let bundle_bytes = self.host.get(&content_id).cloned().unwrap_or_default();
                Some(NetworkMessage::ArtifactResponse {
                    content_id,
                    bundle_bytes,
                })
            }
            _ => None,
        }
    }
}
