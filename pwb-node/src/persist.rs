//! Crash-safe file writes for node state.
//!
//! State files are written to a temporary sibling and renamed into place, so a
//! reader (or a restart after a crash) sees either the previous complete file
//! or the new complete file — never a truncated one.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use pwb_core::crypto::PublicKey;
use serde::{Deserialize, Serialize};

/// Upper bound on remembered peers. The peer table is capped too; this keeps a
/// long-lived node's peer file from growing with every peer it ever met.
pub const MAX_STORED_PEERS: usize = 512;

/// One peer remembered across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPeer {
    pub public_key: PublicKey,
    pub address: SocketAddr,
    /// Unix seconds of the last authenticated message, used to prefer the
    /// most recently live peers when the store is larger than the dial budget.
    #[serde(default)]
    pub last_seen_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct PeerStoreFile {
    version: u32,
    peers: Vec<StoredPeerWire>,
}

/// On-disk form of [`StoredPeer`]. The key is hex, matching connection links
/// and every other key representation in the protocol, so an operator can read
/// and edit the store without decoding a byte array.
#[derive(Serialize, Deserialize)]
struct StoredPeerWire {
    public_key: String,
    address: SocketAddr,
    #[serde(default)]
    last_seen_unix: u64,
}

fn parse_public_key(hex: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(PublicKey(key))
}

/// Load remembered peers from `path`.
///
/// A missing, corrupt, or partially written file yields an empty list: peer
/// memory is an optimisation and must never stop a node from starting. Only
/// dialable addresses are returned, ordered newest-seen first.
pub fn load_peers(path: &Path) -> Vec<StoredPeer> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_slice::<PeerStoreFile>(&bytes) else {
        return Vec::new();
    };
    let mut peers: Vec<StoredPeer> = file
        .peers
        .into_iter()
        .filter_map(|peer| {
            Some(StoredPeer {
                public_key: parse_public_key(&peer.public_key)?,
                address: peer.address,
                last_seen_unix: peer.last_seen_unix,
            })
        })
        .filter(|peer| peer.address.port() != 0 && !peer.address.ip().is_unspecified())
        .collect();
    peers.sort_unstable_by_key(|peer| std::cmp::Reverse(peer.last_seen_unix));
    peers.truncate(MAX_STORED_PEERS);
    peers
}

/// Atomically persist `peers`, deduplicated by public key (first entry wins).
pub fn save_peers(path: &Path, peers: &[StoredPeer]) -> io::Result<()> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for peer in peers {
        if peer.address.port() == 0 || peer.address.ip().is_unspecified() {
            continue;
        }
        if seen.insert(peer.public_key) {
            unique.push(peer.clone());
            if unique.len() == MAX_STORED_PEERS {
                break;
            }
        }
    }
    let file = PeerStoreFile {
        version: 1,
        peers: unique
            .into_iter()
            .map(|peer| StoredPeerWire {
                public_key: peer.public_key.to_hex(),
                address: peer.address,
                last_seen_unix: peer.last_seen_unix,
            })
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    atomic_write(path, &json)
}

/// Atomically replace `path` with `bytes`.
///
/// The temporary file lives in the same directory as the target so the final
/// `rename` stays on one filesystem (and is therefore atomic on Unix).
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_string());
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(".{file_name}.tmp-{}-{unique}", std::process::id());
    let tmp_path = match parent {
        Some(parent) => parent.join(tmp_name),
        None => Path::new(&tmp_name).to_path_buf(),
    };

    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }

    if let Err(error) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pwb-persist-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn peer_store_roundtrips_and_orders_by_last_seen() {
        let dir = temp_path("peers");
        let path = dir.join("peers.json");
        let old = StoredPeer {
            public_key: PublicKey([1; 32]),
            address: "192.0.2.1:9000".parse().unwrap(),
            last_seen_unix: 10,
        };
        let new = StoredPeer {
            public_key: PublicKey([2; 32]),
            address: "192.0.2.2:9001".parse().unwrap(),
            last_seen_unix: 20,
        };
        save_peers(&path, &[old.clone(), new.clone()]).expect("save peers");

        let loaded = load_peers(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], new, "most recently seen peer first");
        assert_eq!(loaded[1], old);

        // Undialable entries are not persisted or loaded.
        let mut unusable = new.clone();
        unusable.address = "0.0.0.0:0".parse().unwrap();
        save_peers(&path, &[unusable]).expect("save empty store");
        assert!(load_peers(&path).is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn peer_store_uses_hex_keys_and_rejects_bad_entries() {
        let dir = temp_path("hexpeers");
        let path = dir.join("peers.json");
        let peer = StoredPeer {
            public_key: PublicKey([0xAB; 32]),
            address: "198.51.100.7:9000".parse().unwrap(),
            last_seen_unix: 3,
        };
        save_peers(&path, std::slice::from_ref(&peer)).expect("save peers");
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&"ab".repeat(32)),
            "the key is written as hex, not a byte array: {raw}"
        );

        // A hand-edited entry with a malformed key is skipped, not fatal.
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["peers"][0]["public_key"] = serde_json::json!("not-hex");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load_peers(&path).is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_peer_store_is_ignored() {
        let dir = temp_path("badpeers");
        let path = dir.join("peers.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, b"{ not json").unwrap();
        assert!(load_peers(&path).is_empty());
        assert!(load_peers(&dir.join("absent.json")).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let dir = std::env::temp_dir().join(format!(
            "pwb-persist-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("state.json");

        atomic_write(&path, b"first").expect("first write");
        assert_eq!(fs::read(&path).unwrap(), b"first");

        atomic_write(&path, b"second").expect("replace");
        assert_eq!(fs::read(&path).unwrap(), b"second");

        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }
}
