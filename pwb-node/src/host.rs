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
