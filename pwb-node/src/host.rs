//! Artifact Hoster & Local Content-Addressed Store (`pwb-node::host`).

use pwb_core::crypto::ContentId;
use pwb_core::wire::Bundle;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::persist::atomic_write;

/// Content-addressed artifact store. Uses interior mutability so a host can be
/// shared (`Arc<ArtifactHost>`) and written to at runtime — the browser stores
/// bundles it resolves and fetches, and the federated search service stores
/// artifacts it retrieves from peers.
pub struct ArtifactHost {
    storage: Mutex<HashMap<ContentId, Vec<u8>>>,
    /// When set, every stored artifact is also written here as
    /// `<content-id>.pwb` and the directory is reloaded on [`Self::open`].
    dir: Option<PathBuf>,
}

impl ArtifactHost {
    pub fn new() -> Self {
        Self {
            storage: Mutex::new(HashMap::new()),
            dir: None,
        }
    }

    /// Open a disk-backed artifact store: create `dir` if needed and load
    /// every valid `.pwb` file in it. Files are named by content ID; the name
    /// is ignored on load (the bytes are re-hashed), so a corrupted or
    /// renamed file can never masquerade as different content.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        let mut storage = HashMap::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("pwb") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if Bundle::deserialize(&bytes).is_err() {
                continue; // ignore corrupt or foreign files
            }
            storage.insert(ContentId::compute(&bytes), bytes);
        }

        Ok(Self {
            storage: Mutex::new(storage),
            dir: Some(dir),
        })
    }

    /// The directory backing this store, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn store(&self, raw_bundle: Vec<u8>) -> Result<ContentId, &'static str> {
        let content_id = ContentId::compute(&raw_bundle);
        if Bundle::deserialize(&raw_bundle).is_err() {
            return Err("Invalid bundle payload");
        }
        self.storage
            .lock()
            .unwrap()
            .insert(content_id, raw_bundle.clone());

        if let Some(dir) = &self.dir {
            let path = dir.join(format!("{}.pwb", content_id.to_hex()));
            if atomic_write(&path, &raw_bundle).is_err() {
                return Err("Failed to persist artifact");
            }
        }
        Ok(content_id)
    }

    pub fn get(&self, content_id: &ContentId) -> Option<Vec<u8>> {
        self.storage.lock().unwrap().get(content_id).cloned()
    }

    pub fn contains(&self, content_id: &ContentId) -> bool {
        self.storage.lock().unwrap().contains_key(content_id)
    }

    pub fn len(&self) -> usize {
        self.storage.lock().unwrap().len()
    }

    /// Every hosted artifact as `(content_id, title)`, sorted by title. Used to
    /// populate the `register.plain` claim console.
    pub fn artifacts(&self) -> Vec<(ContentId, String)> {
        let mut out: Vec<(ContentId, String)> = self
            .storage
            .lock()
            .unwrap()
            .iter()
            .map(|(cid, bytes)| {
                let title = Bundle::deserialize(bytes)
                    .map(|b| b.title)
                    .unwrap_or_default();
                (*cid, title)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    pub fn is_empty(&self) -> bool {
        self.storage.lock().unwrap().is_empty()
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
            author_signature: None,
            root_node: PlainNode::new(0, Tag::Document),
            script_payload: None,
            components: std::collections::HashMap::new(),
            modules: std::collections::HashMap::new(),
        }
        .serialize()
    }

    #[test]
    fn test_store_valid_bundle_returns_content_id_and_gets_it_back() {
        let host = ArtifactHost::new();
        assert!(host.is_empty());
        let bytes = sample_bundle_bytes();
        let content_id = ContentId::compute(&bytes);
        assert_eq!(host.store(bytes.clone()).unwrap(), content_id);
        assert_eq!(host.get(&content_id), Some(bytes));
        assert_eq!(host.len(), 1);
        assert!(!host.is_empty());
    }

    #[test]
    fn test_store_rejects_invalid_bundle_payload() {
        let host = ArtifactHost::new();
        assert_eq!(host.store(vec![0xFF; 8]), Err("Invalid bundle payload"));
        assert!(host.is_empty());
    }

    #[test]
    fn test_store_is_content_addressed_and_deduplicates() {
        let host = ArtifactHost::new();
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

    #[test]
    fn test_open_reloads_persisted_artifacts() {
        let dir = std::env::temp_dir().join(format!(
            "pwb-host-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bytes = sample_bundle_bytes();
        let content_id = ContentId::compute(&bytes);

        {
            let host = ArtifactHost::open(&dir).expect("open store");
            assert!(host.is_empty());
            assert_eq!(host.store(bytes.clone()).unwrap(), content_id);
        }

        let reopened = ArtifactHost::open(&dir).expect("reopen store");
        assert_eq!(
            reopened.get(&content_id),
            Some(bytes),
            "artifact survived restart"
        );
        assert_eq!(reopened.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_open_ignores_corrupt_files() {
        let dir = std::env::temp_dir().join(format!(
            "pwb-host-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("not-a-bundle.pwb"), b"garbage").unwrap();

        let host = ArtifactHost::open(&dir).expect("open store");
        assert!(host.is_empty(), "invalid artifacts are not loaded");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
