//! Identity & Credit Ledger management (`pwb-node::identity`).

use pwb_core::crypto::{Keypair, PublicKey, Signature};

#[derive(Debug, Clone)]
pub struct Account {
    pub pubkey: PublicKey,
    pub credits: u64,
    pub indexing_score: u64,
}

pub struct IdentityManager {
    pub keypair: Keypair,
    pub credits: u64,
}

impl IdentityManager {
    pub fn new() -> Self {
        Self {
            keypair: Keypair::generate(),
            credits: 100, // Initial credit balance for testing
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    pub fn sign_message(&self, message: &[u8]) -> Signature {
        self.keypair.sign(message)
    }

    pub fn add_credits(&mut self, amount: u64) {
        self.credits += amount;
    }

    pub fn spend_credits(&mut self, amount: u64) -> Result<(), &'static str> {
        if self.credits >= amount {
            self.credits -= amount;
            Ok(())
        } else {
            Err("Insufficient credit balance")
        }
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
    fn test_new_identity_starts_with_credits_and_a_keypair() {
        let identity = IdentityManager::new();
        assert_eq!(identity.credits, 100);
        assert_eq!(identity.public_key(), identity.keypair.public);
    }

    #[test]
    fn test_default_matches_new() {
        let identity = IdentityManager::default();
        assert_eq!(identity.credits, 100);
    }

    #[test]
    fn test_sign_message_produces_verifiable_signature() {
        let identity = IdentityManager::new();
        let signature = identity.sign_message(b"hello");
        assert!(signature.verify(&identity.public_key(), b"hello"));
        assert!(!signature.verify(&identity.public_key(), b"other"));
    }

    #[test]
    fn test_add_credits_grows_the_balance() {
        let mut identity = IdentityManager::new();
        identity.add_credits(50);
        assert_eq!(identity.credits, 150);
        identity.add_credits(0);
        assert_eq!(identity.credits, 150);
    }

    #[test]
    fn test_spend_credits_ok_and_insufficient() {
        let mut identity = IdentityManager::new();
        assert!(identity.spend_credits(40).is_ok());
        assert_eq!(identity.credits, 60);
        // Spending the entire balance succeeds…
        assert!(identity.spend_credits(60).is_ok());
        assert_eq!(identity.credits, 0);
        // …but one credit more is refused and the balance is untouched.
        assert_eq!(
            identity.spend_credits(1),
            Err("Insufficient credit balance")
        );
        assert_eq!(identity.credits, 0);
    }

    #[test]
    fn test_account_fields_are_public_data() {
        let account = Account {
            pubkey: PublicKey([3; 32]),
            credits: 10,
            indexing_score: 5,
        };
        assert_eq!(account.credits, 10);
        assert_eq!(account.indexing_score, 5);
    }
}
