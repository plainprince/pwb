//! Artifact Hoster & Local Content-Addressed Store (`pwb-node::host`).

use pwb_core::crypto::ContentId;
use pwb_core::wire::Bundle;
use std::collections::HashMap;

pub struct ArtifactHost {
    storage: HashMap<ContentId, Vec<u8>>,
}

impl ArtifactHost {
    pub fn new() -> Self {
        Self {
            storage: HashMap::new(),
        }
    }

    pub fn store(&mut self, raw_bundle: Vec<u8>) -> Result<ContentId, &'static str> {
        let content_id = ContentId::compute(&raw_bundle);
        if Bundle::deserialize(&raw_bundle).is_err() {
            return Err("Invalid bundle payload");
        }
        self.storage.insert(content_id, raw_bundle);
        Ok(content_id)
    }

    pub fn get(&self, content_id: &ContentId) -> Option<&Vec<u8>> {
        self.storage.get(content_id)
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }
}

impl Default for ArtifactHost {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::{PlainNode, Tag};

    fn sample_bundle_bytes() -> Vec<u8> {
        Bundle {
            version: pwb_core::wire::CURRENT_VERSION,
            title: "Demo".into(),
            author_pubkey: None,
            root_node: PlainNode::new(0, Tag::Document),
            script_payload: None,
            components: std::collections::HashMap::new(),
            modules: std::collections::HashMap::new(),
        }
        .serialize()
    }

    #[test]
    fn test_store_valid_bundle_returns_content_id_and_gets_it_back() {
        let mut host = ArtifactHost::new();
        assert!(host.is_empty());
        let bytes = sample_bundle_bytes();
        let content_id = ContentId::compute(&bytes);
        assert_eq!(host.store(bytes.clone()).unwrap(), content_id);
        assert_eq!(host.get(&content_id), Some(&bytes));
        assert_eq!(host.len(), 1);
        assert!(!host.is_empty());
    }

    #[test]
    fn test_store_rejects_invalid_bundle_payload() {
        let mut host = ArtifactHost::new();
        assert_eq!(host.store(vec![0xFF; 8]), Err("Invalid bundle payload"));
        assert!(host.is_empty());
    }

    #[test]
    fn test_store_is_content_addressed_and_deduplicates() {
        let mut host = ArtifactHost::new();
        let first = host.store(sample_bundle_bytes()).unwrap();
        let second = host.store(sample_bundle_bytes()).unwrap();
        assert_eq!(first, second, "same bytes → same content id");
        assert_eq!(host.len(), 1, "re-stored content does not duplicate");
    }

    #[test]
    fn test_get_unknown_content_id_is_none() {
        let host = ArtifactHost::new();
        assert_eq!(host.get(&ContentId([0; 32])), None);
    }

    #[test]
    fn test_default_host_is_empty() {
        let host = ArtifactHost::default();
        assert!(host.is_empty());
        assert_eq!(host.len(), 0);
    }
}
