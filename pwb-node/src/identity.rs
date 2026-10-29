//! Identity & Credit Ledger management (`pwb-node::identity`).

use std::path::{Path, PathBuf};

use pwb_core::crypto::{Keypair, PublicKey, Signature};

#[derive(Debug, Clone)]
pub struct Account {
    pub pubkey: PublicKey,
    pub indexing_score: u64,
}

/// A node's persisted identity: the Ed25519 keypair and nothing else. Credits
/// live in the signed [`crate::credit::CreditLedger`], never in this file.
#[derive(Clone)]
pub struct IdentityManager {
    pub keypair: Keypair,
}

impl IdentityManager {
    /// Create a fresh ephemeral identity. To keep an identity across restarts,
    /// use [`Self::load_or_create`].
    pub fn new() -> Self {
        Self {
            keypair: Keypair::generate(),
        }
    }

    /// Load the identity at `path`, or create (and persist) a new one there.
    pub fn load_or_create(path: &Path) -> Self {
        if let Some(identity) = Self::load(path) {
            return identity;
        }
        let identity = Self::new();
        let _ = identity.save(path);
        identity
    }

    /// Load a persisted identity; returns `None` when the file is missing or
    /// malformed.
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let secret_hex = value.get("secret_key_hex")?.as_str()?;
        let secret_bytes = hex::decode(secret_hex).ok()?;
        if secret_bytes.len() != 32 {
            return None;
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&secret_bytes);
        Some(Self {
            keypair: Keypair::from_seed(&seed),
        })
    }

    /// Persist this identity's secret key to `path`.
    ///
    /// A legacy `credits` field in an older file is ignored on load; balances
    /// now come from the signed credit ledger.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let value = serde_json::json!({
            "version": 1,
            "public_key": self.keypair.public.to_hex(),
            "secret_key_hex": hex::encode(self.keypair.secret.0),
        });
        let json = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Default on-disk identity path: `$PWB_IDENTITY`, else
    /// `<home>/.pwb/identity.json`, with the home directory resolved
    /// portably (`$HOME`, `$USERPROFILE`, or `%HOMEDRIVE%%HOMEPATH%`).
    /// `$HOME` alone is unset in a stock Windows shell, which used to leave
    /// a Windows node with no identity path at all.
    pub fn default_identity_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("PWB_IDENTITY") {
            return Some(PathBuf::from(p));
        }
        pwb_core::paths::home_dir().map(|home| home.join(".pwb").join("identity.json"))
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    pub fn sign_message(&self, message: &[u8]) -> Signature {
        self.keypair.sign(message)
    }
}

impl Default for IdentityManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_identity_has_a_keypair() {
        let identity = IdentityManager::new();
        assert_eq!(identity.public_key(), identity.keypair.public);
    }

    #[test]
    fn test_default_matches_new() {
        let identity = IdentityManager::default();
        assert_eq!(identity.public_key(), identity.keypair.public);
    }

    #[test]
    fn test_sign_message_produces_verifiable_signature() {
        let identity = IdentityManager::new();
        let signature = identity.sign_message(b"hello");
        assert!(signature.verify(&identity.public_key(), b"hello"));
        assert!(!signature.verify(&identity.public_key(), b"other"));
    }

    #[test]
    fn test_account_fields_are_public_data() {
        let account = Account {
            pubkey: PublicKey([3; 32]),
            indexing_score: 5,
        };
        assert_eq!(account.indexing_score, 5);
    }

    #[test]
    fn test_identity_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("pwb-identity-{}", std::process::id()));
        let path = dir.join("identity.json");
        let identity = IdentityManager::new();
        identity.save(&path).expect("saves identity");
        let loaded = IdentityManager::load(&path).expect("loads identity");
        assert_eq!(loaded.public_key(), identity.public_key());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_legacy_credits_field_is_ignored() {
        let dir = std::env::temp_dir().join(format!("pwb-identity-legacy-{}", std::process::id()));
        let path = dir.join("identity.json");
        let identity = IdentityManager::new();
        let value = serde_json::json!({
            "version": 1,
            "public_key": identity.public_key().to_hex(),
            "secret_key_hex": hex::encode(identity.keypair.secret.0),
            "credits": 1_000_000,
        });
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let loaded = IdentityManager::load(&path).expect("legacy identity loads");
        assert_eq!(loaded.public_key(), identity.public_key());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
